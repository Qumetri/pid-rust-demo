//! Small helpers for the Pi's hardware buses, shared by the sensor and
//! output drivers.
//!
//! - `i2c`: talk to a device on an I2C bus through /dev/i2c-N.
//! - `pwm`: hardware PWM through /sys/class/pwm.
//!
//! Both only exist on Linux. On other systems they return an error that
//! the screen shows, so the window demo still builds on a Mac or PC.

pub mod i2c;
pub mod pwm;
