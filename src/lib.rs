//! The library part of the project.
//!
//! A Cargo package can contain one library plus any number of programs.
//! We now have two programs (the terminal demo and the window demo) that
//! both need the controller and the simulated tank, so that shared code
//! lives here and each program imports it with `use pid_rust_demo::...`.
//!
//! `pub mod` makes src/pid.rs and src/plant.rs part of the library and
//! visible to its users.

pub mod pid;
pub mod plant;
