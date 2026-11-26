# Raspberry Pi PID controller: project notes

These notes collect the decisions, findings and open questions from the planning sessions (12 November 2026), plus
an earlier ESP32 feasibility study (21 October 2026). This repository is the warm-up demo. The notes describe the real
project it prepares for.

Library versions and maintenance status were checked against crates.io and GitHub on **26 November 2026**. Everything
else is from the planning sessions. Hardware behaviour marked *unverified* has not been tried on the real parts yet.

## 1. Goal and requirements

- A PID controller built on a **Raspberry Pi 4B with 1 GB RAM**, in a Fibox EKPE 130 G enclosure.
- **Live graphs and PID controls on a local touchscreen**, with a web page as an optional addition.
- **Demonstration rig:** water pumps, used to show different PID controller options with several sensors.
- **Switchable main sensor**, and **several sensors at once**.
- **External use:** the box should also work as a general-purpose PID controller for an externally connected sensor.
  Sensors added this way do not have to fit the pump demo.
- **Screen:** Raspberry Pi Touch Display 2 (5 inch), 720 × 1280, DSI, with touch.
- **Language:** Rust. The owner knows C++ well and has not used Rust before.

## 2. Hardware (component list, option A)

| Part | Role in the project |
|---|---|
| Raspberry Pi 4B, 1 GB | Controller, UI and web server |
| Raspberry Pi Touch Display 2, 720 × 1280 DSI | Local screen and touch input |
| RCWL-1601 ultrasonic sensor (3–5.5 V) | Water level (trigger pulse, echo timing) |
| YF-S401 flow sensor (5–24 V, 0.3–6 l/min) | Flow (pulse counting) |
| DHT-22 temperature and humidity sensor | Monitoring only (see §6) |
| GY-521 (MPU6050) accelerometer and gyroscope | Role in the rig not decided yet |
| VMA409 motor driver (5–35 V) | Drives the demo pumps with PWM (two channels) |
| MCP4725 DAC | Analogue output, planned as the external controller output |
| Keypad | Number entry, as an alternative to an on-screen pad |
| E-stop, on/off switch, 6 normally closed buttons, 8 green LEDs | Safety and panel controls |
| HDR-15-5 and HDR-60-5 (5 V DIN-rail supplies), fuses, AC connector and cable, DC-to-USB | Power |
| 30 mm 5 V fan | Enclosure cooling |
| WAGO connectors, pin headers, jumper wires, SD card | Wiring and storage |

## 3. Software stack and why

**Recommendation: one Rust program** that runs the control loop, talks to the sensors, draws the local screen directly
(no desktop) and serves the optional web page. There is no Python in the plan.

| Layer | Choice |
|---|---|
| OS | Raspberry Pi OS Lite 64-bit, no desktop |
| Hardware I/O | `rppal` or `linux-embedded-hal` (see §6 on `rppal` being archived) |
| Local UI | Slint on its `linuxkms` backend, with touch input |
| Web add-on | `axum` with a WebSocket, and a static page using uPlot embedded in the binary |
| Config and logs | TOML for profiles, gains and setpoints; CSV for run logs |
| Build | Cross-compile for `aarch64-unknown-linux-gnu`, then copy the binary to the Pi over SSH |

**Alternatives considered:**

- **Python with Qt or pyqtgraph** would fit in 1 GB. The drawback is timing jitter: garbage-collection pauses and
  a GUI sharing the process with the control loop. It is the fallback if Rust proves too steep.
- **A web UI in a Chromium kiosk as the main screen** is too heavy: Chromium alone takes 300–400 MB on a 1 GB board and
  starts slowly.
- **Rust native** uses roughly 30–60 MB of RAM, has no GC pauses, starts in seconds and deploys as one file. All of
  these are estimates; nothing has been measured on the Pi yet.
- **Python for the sensors with Rust for the screen** was asked about and rejected. The ultrasonic and flow sensors
  depend on precise pulse timing, and a separate process would bring the jitter back. It would also add a second
  language and a link between the two programs that can fail. The split is still possible if writing drivers in
  Python is preferred.
- **Go for the core** with the same web page was suggested as a gentler middle route if Rust felt too much.

## 4. Architecture

### Control core

1. **Control thread at a fixed 50–100 Hz** with real-time priority: read the process variable, compute the PID
   output, write the actuator.
2. **PID core features:**
   - derivative on measurement, with a filter;
   - anti-windup;
   - output limits;
   - bumpless auto/manual switching;
   - direct or reverse acting.
3. **A simulated plant** sits behind the same trait as the real hardware. The whole program can then be developed and
   tuned on a normal computer, as this demo does with its water tank.
4. **A ring buffer of samples** feeds both the screen (about 30 fps) and the WebSocket (batched at 10–20 Hz).
5. **Plain threads and channels** in the control core, with no async. `async`/`tokio` stays inside the web server.

### Sensors as interchangeable signals

- **One thread per sensor, each at its own rate.** For example: ultrasonic at 20 Hz, flow at 10 Hz, DHT-22 at 0.5 Hz.
- **Each thread publishes a signal:** value, unit, timestamp and a health flag.
- **The control loop reads signals by name.** Changing the main sensor is a runtime setting, not a code change.
- **Switching sensors is bumpless:** the integral is re-initialised so the pump does not jump.
- **A stale or failed signal drops the loop to manual** with a safe output, and the screen shows why.

### Generic channels and profiles (for the external-controller use)

- **Inputs are generic channels:** anything that yields a number (pulse frequency, echo time, an I2C device).
- **Outputs are generic too:** PWM through the VMA409, or an analogue voltage through the MCP4725.
- **Each input gets two-point calibration** (raw value to engineering units), so an unknown sensor still plots
  meaningfully.
- **A profile ties channels together:** inputs, output, controller mode, gains, limits, direction and safe output
  value.
  - "Pump demo" ships as a ready profile.
  - "External" profiles are created on the screen and saved as TOML.
- **Sensors are chosen from a list of supported drivers.** Adding a new type means adding one small driver file.

### Controller modes to demonstrate

1. **Single loop:** P, PI, PD or PID on any chosen sensor. Anti-windup and the derivative filter can be toggled so
   their effect shows on the graph.
2. **Cascade:** the level controller sets a flow target and a faster inner loop holds that flow. This is the classic
   two-sensor demo and fits the ultrasonic plus flow sensors.
3. **Feedforward:** PID on one sensor plus a correction from a second, such as a measured disturbance flow.
4. **Monitoring only:** other sensors are plotted alongside without affecting the output.

Possible extras later: relay auto-tuning, and saved presets ("sluggish", "aggressive", "oscillating").

### Safety

- **The e-stop must cut actuator power in hardware.** Software only observes it.
- **Outputs go to a safe state on exit or crash,** backed by a watchdog.

### Planned code layout

The code is planned as separate crates in one Cargo workspace:

- `pid`: pure maths, fully unit-tested;
- `plant-sim`;
- `drivers`;
- `ui`;
- `web`.

Start with `pid` and the simulator; that is the easiest Rust in the project.

## 5. The screen on Raspberry Pi OS Lite

Pi OS Lite has no desktop, but the kernel still provides full graphics and input drivers. The program uses them
directly instead of going through a window manager.

- **Display:** the panel is a DRM/KMS device (`/dev/dri/card0`). Slint's `linuxkms` backend opens it, renders with
  the Pi's GPU through OpenGL ES and shows frames fullscreen.
- **Touch:** the touch controller is an input device under `/dev/input/`, which Slint reads through `libinput`.
- **Permissions:** the service user must be in the `video`, `render` and `input` groups.

### Setup steps

1. **Cabling:** connect the panel to the Pi 4's 15-pin DSI connector. The Touch Display 2 box should include a Pi 4
   cable as well as the Pi 5 one; check when it arrives.
2. **Power:** the panel takes 5 V and GND from the GPIO header. Reserve those two pins when planning sensor and fan
   wiring.
3. **Graphics driver:** keep `dtoverlay=vc4-kms-v3d` in `/boot/firmware/config.txt` (the default).
4. **Panel overlay:** current releases normally detect the panel automatically. If not, add
   `dtoverlay=vc4-kms-dsi-ili9881-5inch`. The same overlay also handles touch.
5. **Runtime libraries:** install Mesa (GBM/EGL), `libinput`, `libudev` and `libxkbcommon` with `apt`.
6. **Build and run:** enable Slint's `backend-linuxkms-noseat` feature (available in Slint 1.19) and run with
   `SLINT_BACKEND=linuxkms`.
7. **Rotation:** for landscape mounting, set `SLINT_KMS_ROTATION=90` or `270`.
8. **Autostart:** start the program from a systemd service at boot and disable the login prompt on that screen.

### Things to know

- **Touch rotation is unverified.** It is not known whether touch follows the display rotation automatically. If it
  does not, the panel overlay accepts touch-axis swap and invert options.
- **There is no on-screen keyboard.** Gains and setpoints need a numeric pad drawn in Slint, or the physical keypad.
- **The pixels are small: about 290 per inch.** Run Slint with a scale factor of about 2, which gives a working area of
  roughly 360 × 640 logical pixels (640 × 360 in landscape). That fits one plot and a row of controls per page, so
  plan several simple pages rather than one dense screen:
  - main page: live plot, mode and sensor selectors, setpoint, auto/manual switch;
  - second page: gains and toggles.
- **The same UI runs in a normal window on a Mac or PC,** so it can be built against the simulator first. This demo's
  `src/bin/ui.rs` already works this way.
- **Don't compile on the Pi.** It is slow with 1 GB, so cross-compile on another machine. Cross-compiling against the
  system libraries is a little fiddly to set up the first time.

## 6. Rust libraries for the parts (checked 26 November 2026)

| Part | Crate (latest version) | `embedded-hal` version | Notes |
|---|---|---|---|
| Pi GPIO, I2C, PWM | `rppal` 0.22.1 (December 2024) | 1.0 and 0.2 | **The GitHub repo is archived** (last push July 2025): it still works on the Pi 4 but gets no fixes |
| Pi GPIO, I2C, SPI, serial (fallback) | `linux-embedded-hal` 0.5.0 (August 2026) | 1.0 only | Maintained by the Rust Embedded working group; uses the kernel's standard GPIO character device and `i2c-dev`. No PWM: hardware PWM then goes through the kernel PWM overlay and `/sys/class/pwm` |
| MCP4725 DAC | `mcp4725` 0.4.3 | 0.2 | Writing a value is only a 2–3 byte I2C write, so a small own driver is an option |
| GY-521 (MPU6050) | `mpu6050` 0.1.6 (2022) | 0.2 | Old but usable |
| ADS1115 ADC (if added) | `ads1x1x` 0.3.0 | 1.0 | Well maintained |
| DHT-22 | `dht-sensor` 0.3.0 | 1.0 | Readings sometimes fail under Linux because of timing, so retry, and never use it as a loop's process variable |
| RCWL-1601 ultrasonic | `hc-sr04` 0.1.2 exists (same protocol) | n/a | Plan: write our own (trigger pulse, time the echo with kernel GPIO edge timestamps) |
| YF-S401 flow sensor | none needed | n/a | Count pulses per second |
| VMA409 motor driver | none needed | n/a | One PWM pin and two direction pins per motor |
| Keypad | `keypad` 0.2.2 (2022) | 0.2 | Row and column scanning, easy to write by hand |
| UI | `slint` 1.19.1 | n/a | Has `backend-linuxkms-noseat` |
| Web | `axum` 0.8.9, `tokio` 1.54 | n/a | Web page only |
| Config | `serde` 1.0, `toml` 1.2 | n/a | Profiles and settings |

**Compatibility finding:** `mcp4725`, `mpu6050` and `keypad` are written against **`embedded-hal` 0.2**, while
`linux-embedded-hal` 0.5 provides only 1.0.

- **With `rppal`:** it implements both versions, so all of these drivers plug in as they are. That is the simplest
  start, despite the archive status.
- **With `linux-embedded-hal`:** the 0.2 drivers need a compatibility adapter (for example `embedded-hal-compat`), or
  small drivers of our own. All three devices are simple enough for that.

Most Rust sensor drivers target the common `embedded-hal` interface, so a driver written for a microcontroller also
works on the Pi.

**Hardware gaps:**

- **No analogue inputs.** The Pi has none and the list has no ADC, so an external sensor must be digital (pulses, I2C
  and so on). Most industrial sensors output 0–10 V or 4–20 mA. To support them, add an **ADS1115** (I2C, 4 channels,
  a few euros) with a divider or shunt resistor; the software cost is one driver.
- **No 0–10 V output.** The MCP4725 outputs at most 0–5 V, or 0–3.3 V if powered from the Pi's 3.3 V rail. Driving a
  0–10 V actuator input needs a small op-amp stage.

## 7. Rust learning path (for a C++ programmer)

Topics 1–4 are enough to write the PID core and the simulator.

| # | Topic | C++ comparison | Where it is used |
|---|---|---|---|
| 1 | Ownership, moves, borrowing (`&`, `&mut`) | `unique_ptr` and move semantics by default, with references checked by the compiler | Everywhere; the one genuinely new idea |
| 2 | `struct`, `enum` with data, `match`, `Option`, `Result`, `?` | Replace null, exceptions and `std::variant` | Controller modes, sensor read errors |
| 3 | Traits, generics, `Box<dyn Trait>` | Abstract classes and templates | Sensor and output channels |
| 4 | Cargo, modules, workspaces, `#[test]` | CMake, a package manager and a test framework in one | Project layout, PID tests |
| 5 | Threads, `Arc`, `Mutex`, channels, atomics | `std::thread`, `shared_ptr`, `std::mutex`, but data races are compile errors | Sensor threads, control loop, UI data |
| 6 | `serde` with TOML, `anyhow`/`thiserror` | No direct equivalent | Profiles, config, readable errors |
| 7 | Slint `.slint` markup, properties, callbacks, updating the UI from another thread | Similar to QML | The screen |
| 8 | `async`/`tokio` basics | Coroutines | The web page only, so learn it last |

- **Skip for now:** writing macros, `unsafe`, advanced lifetimes and `Pin`.
- **When lifetime annotations get painful:** let the struct own its data.
- **Material:**
  - The Rust Book, chapters 1–10, then 15–18 when threads and traits come up;
  - Rustlings, for ownership;
  - the Slint tutorial, for topic 7.

**Tooling:**

- `rustup` installs and updates Rust. `rustup target add aarch64-unknown-linux-gnu` adds the Pi target.
- Cargo builds, tests and runs. `cargo add <crate>` adds a dependency.
- `Cargo.lock` pins exact versions, so the Pi build matches the development build.
- Libraries come from crates.io, with documentation on docs.rs.
- Cargo manages only Rust libraries; the system libraries for the screen still come from `apt`.

## 8. Earlier study: ESP32 with a web page (21 October 2026)

Before the Pi, PID on an ESP32 with MicroPython was studied. Its conclusions still matter for the web page:

- **It was feasible:** MicroPython 1.29 with Microdot (asyncio, WebSocket) and uPlot, ideally on an ESP32-S3. The
  board serves the page itself, and graphs and sliders sync both ways.
- **Traffic is tiny.** 100 Hz × 7 channels as float32 is about 3.3 KB/s; a frame every 50 ms is about 166 bytes. The
  bottleneck there was MicroPython's single thread and GC pauses, not Wi-Fi.
- **Realistic loop rate beside a web server:** 50–100 Hz for motors and light, 1–10 Hz for temperature. MicroPython
  has no second core for user code.
- **The JavaScript can stay minimal.** A working page was about 30 lines (2.2 KB): WebSocket, two uPlot graphs,
  sliders, CSV export. The form was generated from a parameter table, so new parameters need no JavaScript changes.
- **Recommended page design:** send batches every 50 ms in a binary format, keep the history in the browser, and keep
  about a 5 s ring buffer on the device.
- **Known problem (reported on GitHub, not reproduced):** Microdot 2.6.0 with MicroPython 1.27 on a classic ESP32 in
  station mode failed to complete TCP handshakes.
- **Don't expose the board to the internet.** If telemetry must leave the local network, send it outgoing only, for
  example MQTT or an occasional HTTPS POST.

The Pi plan keeps the same web design (uPlot, WebSocket, batched binary frames) and moves the rest to Rust on the Pi.

## 9. Status

| Phase | State |
|---|---|
| 0. Warm-up demo (this repo): PID against a simulated tank, terminal and Slint window with live plot and sliders | **Done** (12 November 2026). 12 unit tests pass on Linux. The window was rendered off-screen for the README screenshot; clicking through it on macOS was not tested |
| 1. Skeleton: `pid` crate, simulated plant, unit tests | Not started; the demo's `src/pid.rs` and `src/plant.rs` are the starting point |
| 2. Local UI: live plot of setpoint, measured value and output; gain and setpoint editing | Not started |
| 3. Real drivers: one sensor and one actuator first | Not started |
| 4. Web page | Not started |
| 5. Logging, saved profiles, systemd autostart, safety behaviour, watchdog | Not started |

## 10. Open questions

- How many pumps, and is there one tank or two? This decides the simulator and how realistic the cascade demo is.
- Is the MCP4725 the external controller output, with the VMA409 only for the demo pumps?
- What does the GY-521 measure in the rig?
- Is the screen mounted portrait or landscape?
- Should analogue external sensors (0–10 V, 4–20 mA) be supported? That means adding the ADS1115.
- Is the web page view-only, or may it change gains? Control over the network needs authentication and more care.
- `rppal` (archived, works today, supports both `embedded-hal` versions) or `linux-embedded-hal` (maintained, needs
  adapters or own drivers for three parts)?
