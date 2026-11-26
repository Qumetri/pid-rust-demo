//! The library part of the project.
//!
//! A Cargo package can contain one library plus any number of programs.
//! We have two programs (the terminal demo and the window demo) that both
//! need the controller and the simulated tank, so that shared code lives
//! here and each program imports it with `use pid_rust_demo::...`.
//!
//! `pub mod` makes each file (or folder with a mod.rs) part of the library
//! and visible to its users.

pub mod hw;
pub mod outputs;
pub mod pid;
pub mod plant;
pub mod sensors;
pub mod stats;
