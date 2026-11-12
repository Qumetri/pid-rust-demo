# pid-rust-demo

A small, dependency-free Rust program for getting used to the language
before the Raspberry Pi PID controller project. It runs a PID controller
against a simulated water tank and draws the level in the terminal.

Every file is heavily commented: the comments explain both what the code
does and why it is written that way, with C++ comparisons where useful.

## Setup on a Mac

1. Install Rust (this also installs Cargo, the build tool and package manager):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. Open a new terminal, then check it worked:

   ```sh
   cargo --version
   ```

For editing, VS Code with the `rust-analyzer` extension gives inline
types and errors as you type.

## Running

```sh
cargo run            # PID mode (default)
cargo run -- p       # proportional only
cargo run -- pi      # proportional + integral
cargo test           # run the unit tests
```

Each row shows the time, the tank level, a bar (`#` = level, `|` =
setpoint) and the pump power. The setpoint is 20 cm, then 30 cm from
the halfway point.

What to notice: in `p` mode the level settles about 2 cm below the
setpoint and stays there. In `pi` and `pid` mode it reaches the setpoint.

## Reading order

| File | What it shows |
|---|---|
| `src/pid.rs` | structs, enums, `match`, `Option`, methods, unit tests |
| `src/plant.rs` | traits (interfaces) and implementing them |
| `src/main.rs` | modules, `Result`, `Box<dyn Trait>`, threads, channels, ownership |

## Exercises

Ordered from easy to harder. The compiler's error messages are unusually
helpful, so read them fully when something does not build.

1. Change the gains in `main.rs` until the tank overshoots and oscillates.
2. Add a `Pd` variant to `Mode` in `pid.rs`. The compiler will list every
   place that needs updating.
3. Read the gains from the command line (`cargo run -- pid 0.2 0.05 0.0`).
   `"0.2".parse::<f64>()` returns a `Result`.
4. Add a second plant in `plant.rs`, for example a heater with a slow
   temperature response, and select it with a command-line argument.
   Nothing in `run_control_loop` should need to change.
5. After `thread::spawn` in `main.rs`, try to use `controller` again.
   Read the error: this is the ownership rule that makes threads safe.
